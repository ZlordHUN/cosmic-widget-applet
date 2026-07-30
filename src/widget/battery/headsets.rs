// SPDX-License-Identifier: MPL-2.0

//! Native battery monitoring for headset models supported by HeadsetControl.
//!
//! HeadsetControl remains an optional process-level fallback. This module owns
//! independent Rust HID readers grouped by protocol family.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

#[path = "headsets/audeze.rs"]
mod audeze;
#[path = "headsets/corsair.rs"]
mod corsair;
#[path = "headsets/hyperx.rs"]
mod hyperx;
#[path = "headsets/logitech.rs"]
mod logitech;
#[path = "headsets/misc.rs"]
mod misc;
#[path = "headsets/steelseries.rs"]
mod steelseries;
#[path = "headsets/transport.rs"]
mod transport;

use transport::Reading;

const DISCOVERY_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BatteryState {
    pub(super) name: String,
    pub(super) level: Option<u8>,
    pub(super) status: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct Snapshot {
    pub(super) states: Vec<BatteryState>,
    pub(super) covered_names: Vec<String>,
}

#[derive(Clone, Copy)]
struct Profile {
    vendor_id: u16,
    product_ids: &'static [u16],
    name: &'static str,
    /// `None` mirrors HeadsetControl's interface zero wildcard on Linux.
    interface: Option<i32>,
    query: fn(&HidDevice, u16) -> Result<Reading, String>,
}

pub(super) struct Monitor {
    api: Option<HidApi>,
    last_states: HashMap<String, BatteryState>,
    transient_failures: HashMap<String, u8>,
    last_discovery: Option<Instant>,
}

impl Monitor {
    pub(super) fn new() -> Self {
        Self {
            api: HidApi::new().ok(),
            last_states: HashMap::new(),
            transient_failures: HashMap::new(),
            last_discovery: None,
        }
    }

    pub(super) fn query(&mut self) -> Snapshot {
        if self.api.is_none() {
            self.api = HidApi::new().ok();
        }
        let Some(api) = self.api.as_mut() else {
            return Snapshot::default();
        };

        if self
            .last_discovery
            .is_none_or(|last| last.elapsed() >= DISCOVERY_INTERVAL)
        {
            if let Err(error) = api.refresh_devices() {
                log::debug!("Native headset HID discovery failed: {error}");
            } else {
                self.last_discovery = Some(Instant::now());
            }
        }

        let mut snapshot = Snapshot::default();
        for profile in profiles() {
            let candidates: Vec<_> = api
                .device_list()
                .filter(|device| {
                    device.vendor_id() == profile.vendor_id
                        && profile.product_ids.contains(&device.product_id())
                })
                .collect();
            if candidates.is_empty() {
                self.last_states.remove(profile.name);
                self.transient_failures.remove(profile.name);
                continue;
            }

            let mut last_error = None;
            let mut opened = false;
            let mut state = None;
            for candidate in candidates {
                if profile
                    .interface
                    .is_some_and(|interface| candidate.interface_number() != interface)
                {
                    continue;
                }
                let product_id = candidate.product_id();
                let device = match candidate.open_device(api) {
                    Ok(device) => {
                        opened = true;
                        device
                    }
                    Err(error) => {
                        last_error = Some(format!("could not open HID interface: {error}"));
                        continue;
                    }
                };
                match (profile.query)(&device, product_id) {
                    Ok(reading) => {
                        state = Some(BatteryState {
                            name: profile.name.to_string(),
                            level: reading.level,
                            status: reading.status,
                        });
                        last_error = None;
                        break;
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            if opened {
                snapshot.covered_names.push(profile.name.to_string());
            }

            if let Some(state) = state {
                self.transient_failures.remove(profile.name);
                self.last_states
                    .insert(profile.name.to_string(), state.clone());
                snapshot.states.push(state);
            } else if opened {
                let definitive = last_error
                    .as_deref()
                    .is_some_and(is_definitively_unavailable);
                let failures = self
                    .transient_failures
                    .entry(profile.name.to_string())
                    .or_default();
                *failures = failures.saturating_add(1);
                if definitive || *failures > 1 {
                    self.last_states.remove(profile.name);
                } else if let Some(previous) = self.last_states.get(profile.name) {
                    snapshot.states.push(previous.clone());
                }
            }

            if let Some(error) = last_error {
                log::trace!("Native {} battery query unavailable: {error}", profile.name);
            }
        }
        snapshot
    }
}

fn is_definitively_unavailable(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    ["offline", "powered off", "docked"]
        .iter()
        .any(|marker| error.contains(marker))
}

fn profiles() -> impl Iterator<Item = &'static Profile> {
    audeze::PROFILES
        .iter()
        .chain(corsair::PROFILES)
        .chain(hyperx::PROFILES)
        .chain(logitech::PROFILES)
        .chain(steelseries::PROFILES)
        .chain(misc::PROFILES)
}

pub(super) fn query_audeze_maxwell() -> Result<Option<audeze::BatteryState>, String> {
    audeze::query()
}

#[cfg(test)]
mod tests {
    use super::{is_definitively_unavailable, profiles};
    use std::collections::HashSet;

    #[test]
    fn native_headset_registry_has_unique_usb_ids() {
        let mut ids = HashSet::new();
        for profile in profiles() {
            for product_id in profile.product_ids {
                assert!(
                    ids.insert((profile.vendor_id, *product_id)),
                    "duplicate headset USB ID {:04x}:{product_id:04x}",
                    profile.vendor_id
                );
            }
        }
        assert_eq!(ids.len(), 81);
    }

    #[test]
    fn registry_keeps_first_generation_maxwell_on_its_specialized_reader() {
        let audeze_ids: Vec<_> = profiles()
            .filter(|profile| profile.vendor_id == 0x3329)
            .flat_map(|profile| profile.product_ids)
            .copied()
            .collect();
        assert_eq!(audeze_ids, vec![0x4b29]);
    }

    #[test]
    fn distinguishes_offline_reports_from_transient_transport_errors() {
        assert!(is_definitively_unavailable(
            "Logitech headset is powered off"
        ));
        assert!(is_definitively_unavailable(
            "SteelSeries headset is offline"
        ));
        assert!(!is_definitively_unavailable("HID read timed out"));
    }
}
