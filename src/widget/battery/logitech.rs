// SPDX-License-Identifier: MPL-2.0

//! Native Logitech battery readers for Linux power supplies and HID++ devices.

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "logitech/centurion.rs"]
mod centurion;
#[path = "logitech/protocol.rs"]
mod protocol;
#[path = "logitech/receiver.rs"]
mod receiver;
#[path = "logitech/sysfs.rs"]
mod sysfs;
#[path = "logitech/transport.rs"]
mod transport;

use protocol::{
    BatteryFeature, BatteryProtocol, BatteryReading, DEVICE_FRIENDLY_NAME_FEATURE,
    DEVICE_NAME_FEATURE, HIDPP10_BATTERY_CHARGE_REGISTER, HIDPP10_BATTERY_STATUS_REGISTER,
    parse_hidpp10_battery,
};
use receiver::PairedDevice;
use sysfs::{EndpointKind, HidrawEndpoint};
use transport::{hidpp10_register, hidpp20_request, open as open_hidraw};

const POWER_SUPPLY_ROOT: &str = "/sys/class/power_supply";
const HIDPP_SOFTWARE_ID: u16 = 0;
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(30);
const MAX_UNCONFIRMED_LEVEL_CHANGE: u8 = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BatteryState {
    pub(super) name: String,
    pub(super) level: Option<u8>,
    pub(super) status: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HidppDevice {
    slot: u8,
    name: String,
    kind: Option<String>,
    battery_protocol: BatteryProtocol,
    centurion: Option<centurion::Device>,
}

#[derive(Debug)]
struct MonitoredEndpoint {
    endpoint: HidrawEndpoint,
    devices: Vec<HidppDevice>,
}

pub(super) struct Monitor {
    endpoints: Vec<MonitoredEndpoint>,
    last_readings: HashMap<String, BatteryReading>,
    last_discovery: Option<Instant>,
}

impl Monitor {
    pub(super) fn new() -> Self {
        Self {
            endpoints: Vec::new(),
            last_readings: HashMap::new(),
            last_discovery: None,
        }
    }

    pub(super) fn query(&mut self) -> Vec<BatteryState> {
        let mut states = query_power_supplies_at(Path::new(POWER_SUPPLY_ROOT));

        let discovery_due = self
            .last_discovery
            .is_none_or(|last| last.elapsed() >= DISCOVERY_INTERVAL);
        if discovery_due {
            let discovered = discover_endpoints(&states);
            self.endpoints = reconcile_discovered_endpoints(
                std::mem::take(&mut self.endpoints),
                discovered,
                Path::exists,
            );
            self.last_discovery = self
                .endpoints
                .iter()
                .any(|endpoint| !endpoint.devices.is_empty())
                .then(Instant::now);

            let active_names: Vec<_> = self
                .endpoints
                .iter()
                .flat_map(|endpoint| &endpoint.devices)
                .map(|device| device_identity(&device.name))
                .collect();
            self.last_readings
                .retain(|name, _| active_names.iter().any(|active| active == name));
        }

        for endpoint in &mut self.endpoints {
            let Ok(mut handle) = open_hidraw(&endpoint.endpoint.path) else {
                continue;
            };
            for device in &mut endpoint.devices {
                let identity = device_identity(&device.name);
                let reading = query_confirmed_device_battery(
                    &mut handle,
                    device,
                    self.last_readings.get(&identity),
                );
                if let Some(state) = state_from_reading(device, reading, &mut self.last_readings) {
                    upsert_state(&mut states, state);
                }
            }
        }

        states
    }
}

fn reconcile_discovered_endpoints(
    current: Vec<MonitoredEndpoint>,
    mut discovered: Vec<MonitoredEndpoint>,
    endpoint_is_present: impl Fn(&Path) -> bool,
) -> Vec<MonitoredEndpoint> {
    for previous_endpoint in current {
        let Some(fresh_endpoint) = discovered
            .iter_mut()
            .find(|fresh| fresh.endpoint.path == previous_endpoint.endpoint.path)
        else {
            if endpoint_is_present(&previous_endpoint.endpoint.path) {
                discovered.push(previous_endpoint);
            }
            continue;
        };

        for previous_device in previous_endpoint.devices {
            if let Some(index) = fresh_endpoint
                .devices
                .iter()
                .position(|fresh| fresh.slot == previous_device.slot)
            {
                let fresh_device = fresh_endpoint.devices[index].clone();
                fresh_endpoint.devices[index] =
                    prefer_discovered_device(Some(previous_device), fresh_device);
            } else {
                fresh_endpoint.devices.push(previous_device);
            }
        }
    }

    discovered
}

fn query_confirmed_device_battery(
    handle: &mut File,
    device: &mut HidppDevice,
    previous: Option<&BatteryReading>,
) -> Result<BatteryReading, String> {
    let first = query_device_battery(handle, device)?;
    let Some(previous) = previous else {
        return query_device_battery(handle, device).or(Ok(first));
    };
    if !needs_confirmation(previous, &first) {
        return Ok(first);
    }

    let second = query_device_battery(handle, device)?;
    if readings_agree(&first, &second) || !needs_confirmation(previous, &second) {
        Ok(second)
    } else {
        Err("unconfirmed Logitech battery-level jump".to_string())
    }
}

fn needs_confirmation(previous: &BatteryReading, candidate: &BatteryReading) -> bool {
    match (previous.level, candidate.level) {
        (Some(_), None) => true,
        (Some(previous), Some(candidate)) => {
            previous.abs_diff(candidate) > MAX_UNCONFIRMED_LEVEL_CHANGE
        }
        _ => false,
    }
}

fn readings_agree(left: &BatteryReading, right: &BatteryReading) -> bool {
    match (left.level, right.level) {
        (Some(left), Some(right)) => left.abs_diff(right) <= 2,
        (None, None) => true,
        _ => false,
    }
}

fn state_from_reading(
    device: &HidppDevice,
    reading: Result<BatteryReading, String>,
    last_readings: &mut HashMap<String, BatteryReading>,
) -> Option<BatteryState> {
    let identity = device_identity(&device.name);
    let reading = match reading {
        Ok(reading) => {
            last_readings.insert(identity.clone(), reading.clone());
            reading
        }
        Err(_) => last_readings.get(&identity)?.clone(),
    };

    Some(BatteryState {
        name: device.name.clone(),
        level: reading.level,
        status: reading.status,
        kind: device.kind.clone(),
        connected: true,
    })
}

fn query_power_supplies_at(root: &Path) -> Vec<BatteryState> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    entries
        .filter_map(Result::ok)
        .filter_map(|entry| parse_power_supply(&entry.path()))
        .collect()
}

fn parse_power_supply(path: &Path) -> Option<BatteryState> {
    let manufacturer = read_trimmed(path.join("manufacturer"))?;
    let scope = read_trimmed(path.join("scope"))?;
    if !manufacturer.eq_ignore_ascii_case("Logitech") || !scope.eq_ignore_ascii_case("Device") {
        return None;
    }

    let name = read_trimmed(path.join("model_name"))?;
    let connected = read_trimmed(path.join("online"))
        .map(|online| online != "0")
        .unwrap_or(true);
    let level = connected
        .then(|| read_trimmed(path.join("capacity")))
        .flatten()
        .and_then(|capacity| capacity.parse::<u8>().ok())
        .filter(|capacity| *capacity <= 100);
    let status = connected
        .then(|| read_trimmed(path.join("status")))
        .flatten()
        .and_then(normalize_power_status);

    Some(BatteryState {
        kind: infer_kind(&name),
        name,
        level,
        status,
        connected,
    })
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_power_status(status: String) -> Option<String> {
    match status.to_ascii_lowercase().as_str() {
        "charging" => Some("charging".to_string()),
        "full" => Some("charged".to_string()),
        "discharging" | "not charging" => Some("discharging".to_string()),
        "unknown" => None,
        other => Some(other.to_string()),
    }
}

fn infer_kind(name: &str) -> Option<String> {
    let name = name.to_ascii_lowercase();
    let kind = if name.contains("keyboard") || name.contains("mechanical") || name.contains("keys")
    {
        "keyboard"
    } else if name.contains("numpad") || name.contains("number pad") {
        "numpad"
    } else if name.contains("trackball") || name.contains("ergo") {
        "trackball"
    } else if name.contains("touchpad") {
        "touchpad"
    } else if name.contains("presenter") || name.contains("spotlight") {
        "presenter"
    } else if name.contains("headset")
        || name.contains("headphone")
        || name.contains("zone wireless")
        || name.contains("pro x")
    {
        "headset"
    } else if name.contains("mouse")
        || name.starts_with('g')
            && name[1..].starts_with(|character: char| character.is_ascii_digit())
        || name.contains("master")
        || name.contains("anywhere")
    {
        "mouse"
    } else {
        return None;
    };
    Some(kind.to_string())
}

fn discover_endpoints(power_supply_states: &[BatteryState]) -> Vec<MonitoredEndpoint> {
    sysfs::discover_hidpp_endpoints()
        .into_iter()
        .filter_map(|endpoint| {
            let mut handle = open_hidraw(&endpoint.path).ok()?;
            if let EndpointKind::Centurion(report) = endpoint.kind {
                let device = centurion::discover(&mut handle, report).ok()?;
                let name = centurion_device_name(&endpoint);
                return Some(MonitoredEndpoint {
                    endpoint,
                    devices: vec![HidppDevice {
                        slot: 0xff,
                        kind: Some("headset".to_string()),
                        name,
                        battery_protocol: BatteryProtocol::Unknown,
                        centurion: Some(device),
                    }],
                });
            }

            let paired = match endpoint.kind {
                EndpointKind::Receiver(kind) => receiver::paired_devices(&mut handle, kind).ok()?,
                EndpointKind::Direct => {
                    let name = clean_logitech_name(&endpoint.name);
                    if power_supply_states
                        .iter()
                        .any(|state| device_identity(&state.name) == device_identity(&name))
                    {
                        return None;
                    }
                    vec![PairedDevice {
                        slot: 0xff,
                        name: Some(name),
                        kind: infer_kind(&endpoint.name),
                    }]
                }
                EndpointKind::Centurion(_) => unreachable!(),
            };
            drop(handle);
            let devices = paired
                .into_iter()
                .filter_map(|paired| discover_device_with_retries(&endpoint, paired))
                .collect();

            Some(MonitoredEndpoint { endpoint, devices })
        })
        .collect()
}

fn discover_device_with_retries(
    endpoint: &HidrawEndpoint,
    paired: PairedDevice,
) -> Option<HidppDevice> {
    let attempts = if matches!(endpoint.kind, EndpointKind::Receiver(_)) {
        3
    } else {
        1
    };
    let mut best = None;

    for attempt in 0..attempts {
        let Ok(mut handle) = open_hidraw(&endpoint.path) else {
            break;
        };
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(75));
        }
        let candidate = discover_device(&mut handle, paired.clone());
        let resolved = candidate.battery_protocol != BatteryProtocol::Unknown;
        best = Some(prefer_discovered_device(best, candidate));
        if resolved {
            break;
        }
    }

    best
}

fn prefer_discovered_device(current: Option<HidppDevice>, candidate: HidppDevice) -> HidppDevice {
    let Some(mut current) = current else {
        return candidate;
    };
    if current.battery_protocol == BatteryProtocol::Unknown {
        current.battery_protocol = candidate.battery_protocol;
    }
    if device_name_quality(&candidate.name) > device_name_quality(&current.name) {
        current.name = candidate.name;
    }
    if current.kind.is_none() {
        current.kind = candidate.kind;
    }
    current
}

fn discover_device(handle: &mut File, paired: PairedDevice) -> HidppDevice {
    let slot = paired.slot;
    let name_feature = feature_index(handle, slot, DEVICE_NAME_FEATURE).ok();
    let friendly_name_feature = feature_index(handle, slot, DEVICE_FRIENDLY_NAME_FEATURE).ok();
    let queried_name = name_feature
        .and_then(|feature| query_device_name(handle, slot, feature, false).ok())
        .or_else(|| {
            friendly_name_feature
                .and_then(|feature| query_device_name(handle, slot, feature, true).ok())
        })
        .filter(|name| !name.is_empty());
    let name = [queried_name, paired.name]
        .into_iter()
        .flatten()
        .max_by_key(|name| device_name_quality(name))
        .unwrap_or_else(|| {
            if slot == 0xff {
                "Logitech device".to_string()
            } else {
                format!("Logitech device {slot}")
            }
        });
    let kind = name_feature
        .and_then(|feature| query_device_kind(handle, slot, feature).ok())
        .or(paired.kind)
        .or_else(|| infer_kind(&name));
    let battery_protocol =
        discover_hidpp20_battery_protocol(handle, slot).unwrap_or(BatteryProtocol::Unknown);

    HidppDevice {
        slot,
        name,
        kind,
        battery_protocol,
        centurion: None,
    }
}

fn feature_index(handle: &mut File, slot: u8, feature: u16) -> Result<u8, String> {
    let response = hidpp20_request(handle, slot, HIDPP_SOFTWARE_ID, &feature.to_be_bytes())?;
    response
        .first()
        .copied()
        .filter(|index| *index != 0)
        .ok_or_else(|| format!("HID++ feature {feature:#06x} is unavailable"))
}

fn query_device_name(
    handle: &mut File,
    slot: u8,
    feature: u8,
    friendly: bool,
) -> Result<String, String> {
    let length = hidpp20_request(
        handle,
        slot,
        (u16::from(feature) << 8) | HIDPP_SOFTWARE_ID,
        &[],
    )?
    .first()
    .copied()
    .ok_or_else(|| "HID++ device name length was missing".to_string())? as usize;
    let mut name = Vec::with_capacity(length);

    while name.len() < length {
        let offset =
            u8::try_from(name.len()).map_err(|_| "HID++ device name is too long".to_string())?;
        let fragment = hidpp20_request(
            handle,
            slot,
            (u16::from(feature) << 8) | 0x10 | HIDPP_SOFTWARE_ID,
            &[offset],
        )?;
        if fragment.is_empty() {
            return Err("HID++ device name fragment was empty".to_string());
        }
        let fragment = if friendly {
            fragment.get(1..).unwrap_or_default()
        } else {
            &fragment
        };
        name.extend_from_slice(&fragment[..fragment.len().min(length - name.len())]);
    }

    let name =
        String::from_utf8(name).map_err(|error| format!("invalid HID++ device name: {error}"))?;
    let name = name
        .trim_matches(|character: char| character == '\0' || character.is_whitespace())
        .to_string();
    (!name.is_empty())
        .then_some(name)
        .ok_or_else(|| "HID++ device name was empty".to_string())
}

fn query_device_kind(handle: &mut File, slot: u8, feature: u8) -> Result<String, String> {
    let response = hidpp20_request(
        handle,
        slot,
        (u16::from(feature) << 8) | 0x20 | HIDPP_SOFTWARE_ID,
        &[],
    )?;
    match response.first().copied() {
        Some(0x00) => Ok("keyboard".to_string()),
        Some(0x02) => Ok("numpad".to_string()),
        Some(0x03) => Ok("mouse".to_string()),
        Some(0x04) => Ok("touchpad".to_string()),
        Some(0x05) => Ok("trackball".to_string()),
        Some(0x06) => Ok("presenter".to_string()),
        _ => Err("unknown HID++ device kind".to_string()),
    }
}

fn discover_hidpp20_battery_protocol(handle: &mut File, slot: u8) -> Option<BatteryProtocol> {
    BatteryFeature::ALL.into_iter().find_map(|feature| {
        feature_index(handle, slot, feature.id())
            .ok()
            .map(|index| BatteryProtocol::Hidpp20 { feature, index })
    })
}

fn query_device_battery(
    handle: &mut File,
    device: &mut HidppDevice,
) -> Result<BatteryReading, String> {
    if let Some(centurion) = &device.centurion {
        return centurion::query_battery(handle, centurion);
    }
    match device.battery_protocol {
        BatteryProtocol::Hidpp20 { feature, index } => {
            let response = hidpp20_request(
                handle,
                device.slot,
                (u16::from(index) << 8) | u16::from(feature.function()) | HIDPP_SOFTWARE_ID,
                &[],
            )?;
            feature.parse(&response)
        }
        BatteryProtocol::Hidpp10 => query_hidpp10_battery(handle, device.slot),
        BatteryProtocol::Unknown => {
            if let Some(protocol) = discover_hidpp20_battery_protocol(handle, device.slot) {
                device.battery_protocol = protocol;
                return query_device_battery(handle, device);
            }
            let reading = query_hidpp10_battery(handle, device.slot)?;
            device.battery_protocol = BatteryProtocol::Hidpp10;
            Ok(reading)
        }
    }
}

fn centurion_device_name(endpoint: &HidrawEndpoint) -> String {
    let name = clean_logitech_name(&endpoint.name);
    if !name.eq_ignore_ascii_case("USB Receiver") && !name.eq_ignore_ascii_case("device") {
        return name;
    }
    match endpoint.product_id {
        0x0af7 => "PRO X 2 LIGHTSPEED".to_string(),
        0x0b18 | 0x0b19 => "G522".to_string(),
        _ => "Logitech headset".to_string(),
    }
}

fn query_hidpp10_battery(handle: &mut File, slot: u8) -> Result<BatteryReading, String> {
    for register in [
        HIDPP10_BATTERY_CHARGE_REGISTER,
        HIDPP10_BATTERY_STATUS_REGISTER,
    ] {
        if let Ok(response) = hidpp10_register(handle, slot, register) {
            return parse_hidpp10_battery(register, &response);
        }
    }
    Err("device exposes no supported HID++ battery protocol".to_string())
}

fn clean_logitech_name(name: &str) -> String {
    name.trim()
        .strip_prefix("Logitech, Inc. ")
        .or_else(|| name.trim().strip_prefix("Logitech "))
        .unwrap_or(name.trim())
        .to_string()
}

pub(super) fn device_name_quality(name: &str) -> usize {
    if name.chars().any(char::is_control) {
        return 0;
    }
    let generic_penalty = usize::from(name.starts_with("Logitech device")) * name.len();
    name.trim().chars().count().saturating_sub(generic_penalty)
}

pub(super) fn same_device_name(left: &str, right: &str) -> bool {
    let left = device_identity(left);
    let right = device_identity(right);
    !left.is_empty() && left == right
}

fn device_identity(name: &str) -> String {
    name.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|part| {
            !matches!(
                part.as_str(),
                "logitech" | "logi" | "inc" | "wireless" | "lightspeed"
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn upsert_state(states: &mut Vec<BatteryState>, state: BatteryState) {
    if let Some(existing) = states
        .iter_mut()
        .find(|existing| same_device_name(&existing.name, &state.name))
    {
        if device_name_quality(&state.name) > device_name_quality(&existing.name) {
            existing.name.clone_from(&state.name);
        }
        if existing.level.is_none() {
            existing.level = state.level;
        }
        if existing.status.is_none() {
            existing.status = state.status;
        }
        if existing.kind.is_none() {
            existing.kind = state.kind;
        }
        existing.connected |= state.connected;
    } else {
        states.push(state);
    }
}

#[cfg(test)]
mod tests {
    use super::sysfs::{Bus, EndpointKind, HidrawEndpoint, ReceiverKind};
    use super::{BatteryProtocol, BatteryReading};
    use super::{
        BatteryState, HidppDevice, MonitoredEndpoint, device_identity, device_name_quality,
        infer_kind, needs_confirmation, readings_agree, reconcile_discovered_endpoints,
        same_device_name, state_from_reading, upsert_state,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn infers_current_logitech_device_kinds() {
        assert_eq!(infer_kind("G309 LIGHTSPEED").as_deref(), Some("mouse"));
        assert_eq!(
            infer_kind("MX Mechanical Mini").as_deref(),
            Some("keyboard")
        );
    }

    #[test]
    fn normalizes_transport_marketing_names_for_deduplication() {
        assert_eq!(
            device_identity("Logitech G309 LIGHTSPEED"),
            device_identity("G309")
        );
        assert!(same_device_name("G309", "G309 LIGHTSPEED"));
        assert!(!same_device_name("Logitech LIGHTSPEED", "Wireless"));
    }

    #[test]
    fn deduplication_keeps_the_more_descriptive_device_name() {
        let mut states = vec![BatteryState {
            name: "G309".to_string(),
            level: Some(100),
            status: Some("discharging".to_string()),
            kind: Some("mouse".to_string()),
            connected: true,
        }];

        upsert_state(
            &mut states,
            BatteryState {
                name: "G309 LIGHTSPEED".to_string(),
                level: Some(100),
                status: Some("discharging".to_string()),
                kind: Some("mouse".to_string()),
                connected: true,
            },
        );

        assert_eq!(states.len(), 1);
        assert_eq!(states[0].name, "G309 LIGHTSPEED");
    }

    #[test]
    fn rejects_truncated_control_character_names() {
        assert!(
            device_name_quality("MX Mechanical Mini") > device_name_quality("X Mechanical Mini\0")
        );
    }

    #[test]
    fn requires_large_battery_changes_to_repeat() {
        let previous = BatteryReading {
            level: Some(100),
            status: Some("charged".to_string()),
        };
        let transient = BatteryReading {
            level: Some(18),
            status: Some("discharging".to_string()),
        };
        let confirmed = transient.clone();
        let recovered = previous.clone();

        assert!(needs_confirmation(&previous, &transient));
        assert!(readings_agree(&transient, &confirmed));
        assert!(!readings_agree(&transient, &recovered));
        assert!(!needs_confirmation(&previous, &recovered));
    }

    #[test]
    fn preserves_last_hidpp_reading_while_device_sleeps() {
        let device = HidppDevice {
            slot: 4,
            name: "MX Mechanical Mini".to_string(),
            kind: Some("keyboard".to_string()),
            battery_protocol: BatteryProtocol::Unknown,
            centurion: None,
        };
        let mut readings = HashMap::new();

        let awake = state_from_reading(
            &device,
            Ok(BatteryReading {
                level: Some(20),
                status: Some("discharging".to_string()),
            }),
            &mut readings,
        )
        .unwrap();
        let sleeping =
            state_from_reading(&device, Err("device timed out".to_string()), &mut readings)
                .unwrap();

        assert_eq!(sleeping.level, awake.level);
        assert_eq!(sleeping.status, awake.status);
        assert!(sleeping.connected);
    }

    #[test]
    fn preserves_a_sleeping_receiver_device_during_rediscovery() {
        let endpoint = HidrawEndpoint {
            path: PathBuf::from("/dev/hidraw-bolt-test"),
            bus: Bus::Usb,
            product_id: 0xc548,
            name: "Logitech USB Receiver".to_string(),
            kind: EndpointKind::Receiver(ReceiverKind::Bolt),
        };
        let known_keyboard = HidppDevice {
            slot: 1,
            name: "MX Mechanical Mini".to_string(),
            kind: Some("keyboard".to_string()),
            battery_protocol: BatteryProtocol::Hidpp20 {
                feature: super::BatteryFeature::Unified,
                index: 4,
            },
            centurion: None,
        };
        let current = vec![MonitoredEndpoint {
            endpoint: endpoint.clone(),
            devices: vec![known_keyboard.clone()],
        }];

        let retained = reconcile_discovered_endpoints(current, Vec::new(), |_| true);

        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].devices, vec![known_keyboard]);

        let removed = reconcile_discovered_endpoints(retained, Vec::new(), |_| false);
        assert!(removed.is_empty());
    }

    #[test]
    #[ignore = "requires connected Logitech devices"]
    fn reads_connected_logitech_devices() {
        let started = std::time::Instant::now();
        let power_supply_states =
            super::query_power_supplies_at(std::path::Path::new(super::POWER_SUPPLY_ROOT));
        let mut endpoints = super::discover_endpoints(&power_supply_states);
        println!("Discovered Logitech HID++ endpoints: {endpoints:#?}");
        for endpoint in &mut endpoints {
            let mut handle = super::open_hidraw(&endpoint.endpoint.path).unwrap();
            for device in &mut endpoint.devices {
                let reading = super::query_device_battery(&mut handle, device);
                println!("{}: {reading:?}", device.name);
            }
        }
        let mut monitor = super::Monitor::new();
        let states = monitor.query();
        println!(
            "Logitech native battery states in {:?}: {states:?}",
            started.elapsed()
        );
        assert!(!states.is_empty());
        assert!(states.iter().any(|state| state.name == "G309 LIGHTSPEED"));
        assert!(
            states
                .iter()
                .any(|state| state.name == "MX Mechanical Mini")
        );
        for state in &states {
            assert!(state.level.is_none_or(|level| level <= 100));
        }
    }
}
