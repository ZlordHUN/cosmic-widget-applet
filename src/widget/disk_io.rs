// SPDX-License-Identifier: MPL-2.0

//! Aggregate physical-disk throughput from Linux block statistics.

use std::collections::HashSet;
use std::fs;
use std::time::{Duration, Instant};

const DEVICE_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const KERNEL_SECTOR_SIZE: u64 = 512;

pub struct DiskIoMonitor {
    devices: HashSet<String>,
    read_bytes: u64,
    written_bytes: u64,
    last_update: Instant,
    last_device_refresh: Instant,
    pub read_rate: f64,
    pub write_rate: f64,
}

impl DiskIoMonitor {
    pub fn new() -> Self {
        let devices = physical_block_devices();
        let (read_bytes, written_bytes) = read_totals(&devices).unwrap_or_default();
        let now = Instant::now();

        Self {
            devices,
            read_bytes,
            written_bytes,
            last_update: now,
            last_device_refresh: now,
            read_rate: 0.0,
            write_rate: 0.0,
        }
    }

    pub fn update(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();

        if now.duration_since(self.last_device_refresh) >= DEVICE_REFRESH_INTERVAL {
            let devices = physical_block_devices();
            if devices != self.devices {
                self.devices = devices;
                (self.read_bytes, self.written_bytes) =
                    read_totals(&self.devices).unwrap_or_default();
                self.read_rate = 0.0;
                self.write_rate = 0.0;
                self.last_update = now;
                self.last_device_refresh = now;
                return;
            }
            self.last_device_refresh = now;
        }

        let Some((read_bytes, written_bytes)) = read_totals(&self.devices) else {
            self.read_rate = 0.0;
            self.write_rate = 0.0;
            self.last_update = now;
            return;
        };

        self.read_rate = rate_from_totals(self.read_bytes, read_bytes, elapsed);
        self.write_rate = rate_from_totals(self.written_bytes, written_bytes, elapsed);
        self.read_bytes = read_bytes;
        self.written_bytes = written_bytes;
        self.last_update = now;
    }
}

fn physical_block_devices() -> HashSet<String> {
    let Ok(entries) = fs::read_dir("/sys/class/block") else {
        return HashSet::new();
    };

    entries
        .flatten()
        .filter(|entry| entry.path().join("device").exists())
        .filter(|entry| !entry.path().join("partition").exists())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

fn read_totals(devices: &HashSet<String>) -> Option<(u64, u64)> {
    let diskstats = fs::read_to_string("/proc/diskstats").ok()?;
    Some(parse_diskstats(&diskstats, devices))
}

fn parse_diskstats(diskstats: &str, devices: &HashSet<String>) -> (u64, u64) {
    diskstats.lines().fold((0_u64, 0_u64), |totals, line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 10 || !devices.contains(fields[2]) {
            return totals;
        }

        let sectors_read = fields[5].parse::<u64>().unwrap_or(0);
        let sectors_written = fields[9].parse::<u64>().unwrap_or(0);
        (
            totals
                .0
                .saturating_add(sectors_read.saturating_mul(KERNEL_SECTOR_SIZE)),
            totals
                .1
                .saturating_add(sectors_written.saturating_mul(KERNEL_SECTOR_SIZE)),
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
    use super::{parse_diskstats, rate_from_totals};
    use std::collections::HashSet;

    #[test]
    fn sums_only_selected_physical_devices() {
        let devices = HashSet::from(["nvme0n1".to_string(), "sda".to_string()]);
        let fixture = "\
259 0 nvme0n1 1 0 100 0 2 0 50 0 0 0 0 0 0 0 0 0\n\
259 1 nvme0n1p1 1 0 75 0 2 0 30 0 0 0 0 0 0 0 0 0\n\
8 0 sda 1 0 20 0 2 0 10 0 0 0 0 0 0 0 0 0\n";

        assert_eq!(parse_diskstats(fixture, &devices), (120 * 512, 60 * 512));
    }

    #[test]
    fn calculates_rates_and_rejects_counter_resets() {
        assert_eq!(rate_from_totals(1_000, 3_000, 0.5), 4_000.0);
        assert_eq!(rate_from_totals(3_000, 1_000, 1.0), 0.0);
        assert_eq!(rate_from_totals(1_000, 3_000, 0.0), 0.0);
    }
}
