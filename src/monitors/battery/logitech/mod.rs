// SPDX-License-Identifier: MPL-2.0

//! Native Logitech battery readers for Linux power supplies and HID++ devices.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod centurion;
mod protocol;
mod receiver;
mod sysfs;
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
const MAX_BATTERY_EVENT_REPORTS: usize = 64;

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
    pairing_name: Option<String>,
    kind: Option<String>,
    battery_protocol: BatteryProtocol,
    centurion: Option<centurion::Device>,
}

#[derive(Debug)]
struct MonitoredEndpoint {
    endpoint: HidrawEndpoint,
    devices: Vec<HidppDevice>,
}

struct EventEndpoint {
    endpoint: HidrawEndpoint,
    handle: File,
}

pub(super) struct Monitor {
    endpoints: Vec<MonitoredEndpoint>,
    event_endpoints: Vec<EventEndpoint>,
    last_readings: HashMap<String, BatteryReading>,
    pending_event_readings: HashMap<String, BatteryReading>,
    last_discovery: Option<Instant>,
}

impl Monitor {
    pub(super) fn new() -> Self {
        Self {
            endpoints: Vec::new(),
            event_endpoints: Vec::new(),
            last_readings: HashMap::new(),
            pending_event_readings: HashMap::new(),
            last_discovery: None,
        }
    }

    pub(super) fn query(&mut self) -> Vec<BatteryState> {
        let mut states = query_power_supplies_at(Path::new(POWER_SUPPLY_ROOT));

        let discovery_due = self
            .last_discovery
            .is_none_or(|last| last.elapsed() >= DISCOVERY_INTERVAL);
        if discovery_due {
            let present_endpoints = sysfs::discover_hidpp_endpoints();
            self.reconcile_event_endpoints(&present_endpoints);
            let discovered =
                discover_endpoints_with_known(&states, &present_endpoints, &self.endpoints);
            self.endpoints = reconcile_discovered_endpoints(
                std::mem::take(&mut self.endpoints),
                discovered,
                |endpoint| present_endpoints.contains(endpoint),
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
            self.pending_event_readings
                .retain(|name, _| active_names.iter().any(|active| active == name));
        }

        self.read_battery_events(&mut states);
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
                let reading_is_live = reading.is_ok();
                if reading_is_live {
                    self.pending_event_readings.remove(&identity);
                }
                if let Some(state) = state_from_reading(device, reading, &mut self.last_readings) {
                    upsert_state(&mut states, state, reading_is_live);
                }
            }
        }
        self.read_battery_events(&mut states);

        states
    }

    fn reconcile_event_endpoints(&mut self, present_endpoints: &[HidrawEndpoint]) {
        self.event_endpoints
            .retain(|listener| present_endpoints.contains(&listener.endpoint));
        for endpoint in present_endpoints {
            if matches!(endpoint.kind, EndpointKind::Centurion(_))
                || self
                    .event_endpoints
                    .iter()
                    .any(|listener| listener.endpoint == *endpoint)
            {
                continue;
            }
            // Keep an independent hidraw input queue open between polls. Request
            // handles deliberately drain unrelated reports and cannot retain the
            // battery event a keyboard emits before going back to sleep.
            if let Ok(handle) = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&endpoint.path)
            {
                self.event_endpoints.push(EventEndpoint {
                    endpoint: endpoint.clone(),
                    handle,
                });
            }
        }
    }

    fn read_battery_events(&mut self, states: &mut Vec<BatteryState>) {
        let mut closed = false;
        self.event_endpoints.retain_mut(|listener| {
            let devices = self
                .endpoints
                .iter()
                .find(|endpoint| endpoint.endpoint == listener.endpoint)
                .map(|endpoint| endpoint.devices.as_slice())
                .unwrap_or_default();
            let present = drain_battery_events(
                &mut listener.handle,
                devices,
                &mut self.last_readings,
                &mut self.pending_event_readings,
                states,
            );
            closed |= !present;
            present
        });
        if closed {
            self.last_discovery = None;
        }
    }
}

fn drain_battery_events(
    handle: &mut impl Read,
    devices: &[HidppDevice],
    last_readings: &mut HashMap<String, BatteryReading>,
    pending_readings: &mut HashMap<String, BatteryReading>,
    states: &mut Vec<BatteryState>,
) -> bool {
    for _ in 0..MAX_BATTERY_EVENT_REPORTS {
        let mut report = [0; 64];
        let read = match handle.read(&mut report) {
            Ok(0) => return false,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        };
        for device in devices {
            let Some(reading) = parse_battery_notification(device, &report[..read]) else {
                continue;
            };
            if let Some(state) =
                apply_battery_notification(device, reading, last_readings, pending_readings)
            {
                upsert_state(states, state, true);
            }
        }
    }
    true
}

fn parse_battery_notification(device: &HidppDevice, report: &[u8]) -> Option<BatteryReading> {
    match report.first() {
        Some(0x10) if report.len() == 7 => {}
        Some(0x11) if report.len() == 20 => {}
        _ => return None,
    }
    if (report[1] != device.slot && !(device.slot == 0xff && report[1] == 0)) || report[3] != 0 {
        return None;
    }
    let BatteryProtocol::Hidpp20 { feature, index } = device.battery_protocol else {
        return None;
    };
    if index == 0 || report[2] != index {
        return None;
    }
    match feature {
        BatteryFeature::Status | BatteryFeature::Voltage | BatteryFeature::AdcMeasurement => {}
        BatteryFeature::Unified if report.len() >= 8 => {}
        _ => return None,
    }
    feature.parse(&report[4..]).ok()
}

fn apply_battery_notification(
    device: &HidppDevice,
    mut reading: BatteryReading,
    last_readings: &mut HashMap<String, BatteryReading>,
    pending_readings: &mut HashMap<String, BatteryReading>,
) -> Option<BatteryState> {
    let identity = device_identity(&device.name);
    if let Some(previous) = last_readings.get(&identity)
        && needs_confirmation(previous, &reading)
    {
        let confirmed = pending_readings
            .remove(&identity)
            .and_then(|first| confirm_repeated_reading(previous, first, reading.clone()).ok());
        if let Some(confirmed) = confirmed {
            reading = confirmed;
        } else {
            pending_readings.insert(identity, reading.clone());
            // Confirm suspicious percentages independently: a valid status
            // event must still update the charging indicator immediately.
            reading.level = previous.level;
            return state_from_reading(device, Ok(reading), last_readings);
        }
    }
    pending_readings.remove(&identity);
    state_from_reading(device, Ok(reading), last_readings)
}

fn reconcile_discovered_endpoints(
    current: Vec<MonitoredEndpoint>,
    mut discovered: Vec<MonitoredEndpoint>,
    endpoint_is_present: impl Fn(&HidrawEndpoint) -> bool,
) -> Vec<MonitoredEndpoint> {
    for previous_endpoint in current {
        let Some(fresh_endpoint) = discovered
            .iter_mut()
            .find(|fresh| fresh.endpoint.path == previous_endpoint.endpoint.path)
        else {
            if endpoint_is_present(&previous_endpoint.endpoint) {
                discovered.push(previous_endpoint);
            }
            continue;
        };
        if fresh_endpoint.endpoint != previous_endpoint.endpoint {
            continue;
        }

        for previous_device in previous_endpoint.devices {
            if let Some(index) = fresh_endpoint
                .devices
                .iter()
                .position(|fresh| fresh.slot == previous_device.slot)
            {
                let fresh_device = fresh_endpoint.devices[index].clone();
                if same_pairing(&previous_device, &fresh_device) {
                    fresh_endpoint.devices[index] =
                        prefer_discovered_device(Some(previous_device), fresh_device);
                }
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
        return Ok(first);
    };
    if !needs_confirmation(previous, &first) {
        return Ok(first);
    }

    let second = query_device_battery(handle, device)?;
    confirm_repeated_reading(previous, first, second)
}

fn confirm_repeated_reading(
    previous: &BatteryReading,
    first: BatteryReading,
    second: BatteryReading,
) -> Result<BatteryReading, String> {
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

#[cfg(test)]
fn discover_endpoints(power_supply_states: &[BatteryState]) -> Vec<MonitoredEndpoint> {
    discover_endpoints_with_known(power_supply_states, &sysfs::discover_hidpp_endpoints(), &[])
}

fn discover_endpoints_with_known(
    power_supply_states: &[BatteryState],
    present_endpoints: &[HidrawEndpoint],
    known: &[MonitoredEndpoint],
) -> Vec<MonitoredEndpoint> {
    present_endpoints
        .iter()
        .cloned()
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
                        pairing_name: None,
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
            let devices =
                discover_paired_devices(&endpoint, paired, known, discover_device_with_retries);

            Some(MonitoredEndpoint { endpoint, devices })
        })
        .collect()
}

fn discover_paired_devices(
    endpoint: &HidrawEndpoint,
    paired: Vec<PairedDevice>,
    known: &[MonitoredEndpoint],
    mut discover: impl FnMut(&HidrawEndpoint, PairedDevice) -> Option<HidppDevice>,
) -> Vec<HidppDevice> {
    let previous = known.iter().find(|known| known.endpoint == *endpoint);
    paired
        .into_iter()
        .filter_map(|paired| {
            // Feature indices and device names are stable for a paired device.
            // Re-querying them on every scan stalls battery polling when another
            // paired device sleeps. Receiver metadata still detects changed slots.
            let resolved = previous.and_then(|previous| {
                previous.devices.iter().find(|device| {
                    device.battery_protocol != BatteryProtocol::Unknown
                        && device.slot == paired.slot
                        && pairing_names_match(
                            device.pairing_name.as_deref(),
                            paired.name.as_deref(),
                        )
                        && compatible_device_kinds(&device.kind, &paired.kind)
                })
            });
            resolved.cloned().or_else(|| discover(endpoint, paired))
        })
        .collect()
}

fn pairing_names_match(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            device_name_quality(left) > 0
                && device_name_quality(right) > 0
                && same_device_name(left, right)
        }
        _ => false,
    }
}

fn compatible_device_kinds(left: &Option<String>, right: &Option<String>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        _ => true,
    }
}

fn same_pairing(left: &HidppDevice, right: &HidppDevice) -> bool {
    left.slot == right.slot
        && pairing_names_match(left.pairing_name.as_deref(), right.pairing_name.as_deref())
        && compatible_device_kinds(&left.kind, &right.kind)
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
    let pairing_name = paired.name.clone();
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
        pairing_name,
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

fn upsert_state(states: &mut Vec<BatteryState>, state: BatteryState, prefer_reading: bool) {
    if let Some(existing) = states
        .iter_mut()
        .find(|existing| same_device_name(&existing.name, &state.name))
    {
        if device_name_quality(&state.name) > device_name_quality(&existing.name) {
            existing.name.clone_from(&state.name);
        }
        if prefer_reading || existing.level.is_none() {
            existing.level = state.level;
        }
        if prefer_reading || existing.status.is_none() {
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
    use super::receiver::PairedDevice;
    use super::sysfs::{Bus, EndpointKind, HidrawEndpoint, ReceiverKind};
    use super::{BatteryProtocol, BatteryReading};
    use super::{
        BatteryState, HidppDevice, MAX_BATTERY_EVENT_REPORTS, MonitoredEndpoint,
        apply_battery_notification, confirm_repeated_reading, device_identity, device_name_quality,
        discover_paired_devices, drain_battery_events, infer_kind, needs_confirmation,
        parse_battery_notification, readings_agree, reconcile_discovered_endpoints,
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
            true,
        );

        assert_eq!(states.len(), 1);
        assert_eq!(states[0].name, "G309 LIGHTSPEED");
    }

    #[test]
    fn live_hidpp_reading_overrides_generic_power_supply_value() {
        let mut states = vec![BatteryState {
            name: "G309 LIGHTSPEED".to_string(),
            level: Some(63),
            status: Some("discharging".to_string()),
            kind: Some("mouse".to_string()),
            connected: true,
        }];

        upsert_state(
            &mut states,
            BatteryState {
                name: "G309 LIGHTSPEED".to_string(),
                level: Some(100),
                status: Some("charged".to_string()),
                kind: Some("mouse".to_string()),
                connected: true,
            },
            true,
        );

        assert_eq!(states[0].level, Some(100));
        assert_eq!(states[0].status.as_deref(), Some("charged"));
    }

    #[test]
    fn remembered_hidpp_reading_does_not_override_live_power_supply_value() {
        let mut states = vec![BatteryState {
            name: "MX Mechanical Mini".to_string(),
            level: Some(80),
            status: Some("discharging".to_string()),
            kind: Some("keyboard".to_string()),
            connected: true,
        }];

        upsert_state(
            &mut states,
            BatteryState {
                name: "MX Mechanical Mini".to_string(),
                level: Some(65),
                status: Some("discharging".to_string()),
                kind: Some("keyboard".to_string()),
                connected: true,
            },
            false,
        );

        assert_eq!(states[0].level, Some(80));
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
    fn confirms_large_battery_changes_against_a_second_reading() {
        let previous = BatteryReading {
            level: Some(85),
            status: Some("discharging".to_string()),
        };
        let transient = BatteryReading {
            level: Some(1),
            status: Some("discharging".to_string()),
        };
        let recovered = previous.clone();
        let conflicting = BatteryReading {
            level: Some(50),
            status: Some("discharging".to_string()),
        };

        assert_eq!(
            confirm_repeated_reading(&previous, transient.clone(), recovered.clone()),
            Ok(recovered)
        );
        assert_eq!(
            confirm_repeated_reading(&previous, transient.clone(), transient.clone()),
            Ok(transient.clone())
        );
        assert!(
            confirm_repeated_reading(&previous, transient, conflicting).is_err(),
            "two contradictory large changes must not become authoritative"
        );
    }

    #[test]
    fn preserves_last_hidpp_reading_while_device_sleeps() {
        let device = HidppDevice {
            slot: 4,
            name: "MX Mechanical Mini".to_string(),
            pairing_name: Some("KEYS".to_string()),
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

    fn bolt_endpoint() -> HidrawEndpoint {
        HidrawEndpoint {
            path: PathBuf::from("/dev/hidraw-bolt-test"),
            bus: Bus::Usb,
            product_id: 0xc548,
            name: "Logitech USB Receiver".to_string(),
            kind: EndpointKind::Receiver(ReceiverKind::Bolt),
        }
    }

    fn resolved_keyboard() -> HidppDevice {
        HidppDevice {
            slot: 1,
            name: "MX Mechanical Mini".to_string(),
            pairing_name: Some("KEYS".to_string()),
            kind: Some("keyboard".to_string()),
            battery_protocol: BatteryProtocol::Hidpp20 {
                feature: super::BatteryFeature::Unified,
                index: 4,
            },
            centurion: None,
        }
    }

    fn keyboard_pairing() -> PairedDevice {
        PairedDevice {
            slot: 1,
            name: Some("KEYS".to_string()),
            kind: Some("keyboard".to_string()),
        }
    }

    fn keyboard_battery_event(level: u8, status: u8) -> [u8; 20] {
        let mut report = [0; 20];
        report[..8].copy_from_slice(&[0x11, 1, 4, 0, level, 4, status, 0]);
        report
    }

    #[test]
    fn accepts_only_matching_complete_battery_events() {
        let keyboard = resolved_keyboard();
        let report = keyboard_battery_event(20, 1);
        assert_eq!(
            parse_battery_notification(&keyboard, &report),
            Some(BatteryReading {
                level: Some(20),
                status: Some("charging".to_string())
            })
        );
        for length in 0..report.len() {
            assert!(parse_battery_notification(&keyboard, &report[..length]).is_none());
        }
        for (offset, value) in [(0, 0x12), (1, 2), (2, 5), (3, 1), (3, 0x10), (6, 0xff)] {
            let mut unrelated = report;
            unrelated[offset] = value;
            assert!(parse_battery_notification(&keyboard, &unrelated).is_none());
        }
        let unknown = HidppDevice {
            battery_protocol: BatteryProtocol::Unknown,
            ..keyboard
        };
        assert!(parse_battery_notification(&unknown, &report).is_none());
    }

    #[test]
    fn charging_and_unplug_events_update_a_sleeping_keyboard() {
        let keyboard = resolved_keyboard();
        let mut readings = HashMap::new();
        let mut pending = HashMap::new();
        state_from_reading(
            &keyboard,
            Ok(BatteryReading {
                level: Some(20),
                status: Some("discharging".to_string()),
            }),
            &mut readings,
        );

        for (status, expected) in [(1, "charging"), (0, "discharging")] {
            let reading =
                parse_battery_notification(&keyboard, &keyboard_battery_event(20, status)).unwrap();
            let event_state =
                apply_battery_notification(&keyboard, reading, &mut readings, &mut pending)
                    .unwrap();
            let sleeping =
                state_from_reading(&keyboard, Err("device asleep".to_string()), &mut readings)
                    .unwrap();
            assert_eq!(event_state, sleeping);
            assert_eq!(sleeping.level, Some(20));
            assert_eq!(sleeping.status.as_deref(), Some(expected));
        }
    }

    #[test]
    fn battery_events_confirm_level_jumps_without_delaying_charging_status() {
        let keyboard = resolved_keyboard();
        let mut readings = HashMap::new();
        let mut pending = HashMap::new();
        state_from_reading(
            &keyboard,
            Ok(BatteryReading {
                level: Some(20),
                status: Some("discharging".to_string()),
            }),
            &mut readings,
        );
        let reading =
            parse_battery_notification(&keyboard, &keyboard_battery_event(80, 1)).unwrap();

        let first =
            apply_battery_notification(&keyboard, reading.clone(), &mut readings, &mut pending)
                .unwrap();
        assert_eq!(first.level, Some(20));
        assert_eq!(first.status.as_deref(), Some("charging"));
        let confirmed =
            apply_battery_notification(&keyboard, reading, &mut readings, &mut pending).unwrap();
        assert_eq!(confirmed.level, Some(80));
        assert_eq!(confirmed.status.as_deref(), Some("charging"));
        assert!(pending.is_empty());
    }

    #[test]
    fn bounds_event_draining_when_other_clients_keep_sending_reports() {
        struct Flood {
            reads: usize,
        }
        impl std::io::Read for Flood {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                buffer[..20].copy_from_slice(&keyboard_battery_event(20, 1));
                Ok(20)
            }
        }
        let mut flood = Flood { reads: 0 };
        let mut states = Vec::new();

        assert!(drain_battery_events(
            &mut flood,
            &[resolved_keyboard()],
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut states,
        ));
        assert_eq!(flood.reads, MAX_BATTERY_EVENT_REPORTS);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].status.as_deref(), Some("charging"));
    }

    #[test]
    fn reuses_resolved_features_without_waking_a_paired_device() {
        let endpoint = bolt_endpoint();
        let keyboard = resolved_keyboard();
        let known = vec![MonitoredEndpoint {
            endpoint: endpoint.clone(),
            devices: vec![keyboard.clone()],
        }];

        let devices =
            discover_paired_devices(&endpoint, vec![keyboard_pairing()], &known, |_, _| {
                panic!("a resolved pairing must not repeat device discovery")
            });

        assert_eq!(devices, vec![keyboard]);
    }

    #[test]
    fn discovers_new_unknown_and_changed_pairings() {
        let endpoint = bolt_endpoint();
        let keyboard = resolved_keyboard();
        let mut unknown = keyboard.clone();
        unknown.battery_protocol = BatteryProtocol::Unknown;
        let cases = [
            (unknown, keyboard_pairing()),
            (
                keyboard.clone(),
                PairedDevice {
                    slot: 2,
                    ..keyboard_pairing()
                },
            ),
            (
                keyboard.clone(),
                PairedDevice {
                    name: Some("MX Keys Mini".to_string()),
                    ..keyboard_pairing()
                },
            ),
            (
                keyboard.clone(),
                PairedDevice {
                    name: None,
                    ..keyboard_pairing()
                },
            ),
            (
                keyboard,
                PairedDevice {
                    kind: Some("mouse".to_string()),
                    ..keyboard_pairing()
                },
            ),
        ];

        for (previous, paired) in cases {
            let known = vec![MonitoredEndpoint {
                endpoint: endpoint.clone(),
                devices: vec![previous],
            }];
            let mut probes = Vec::new();
            discover_paired_devices(&endpoint, vec![paired.clone()], &known, |_, candidate| {
                probes.push(candidate);
                None
            });
            assert_eq!(probes, vec![paired]);
        }
    }

    #[test]
    fn discovers_again_when_an_endpoint_changes() {
        let endpoint = bolt_endpoint();
        let known = vec![MonitoredEndpoint {
            endpoint: endpoint.clone(),
            devices: vec![resolved_keyboard()],
        }];
        let changed_endpoints = [
            HidrawEndpoint {
                path: PathBuf::from("/dev/hidraw-new"),
                ..endpoint.clone()
            },
            HidrawEndpoint {
                product_id: 0xc52b,
                ..endpoint.clone()
            },
            HidrawEndpoint {
                bus: Bus::Bluetooth,
                ..endpoint.clone()
            },
            HidrawEndpoint {
                name: "Replacement Receiver".to_string(),
                ..endpoint.clone()
            },
            HidrawEndpoint {
                kind: EndpointKind::Direct,
                ..endpoint
            },
        ];

        for endpoint in changed_endpoints {
            let mut probes = Vec::new();
            discover_paired_devices(&endpoint, vec![keyboard_pairing()], &known, |_, paired| {
                probes.push(paired);
                None
            });
            assert_eq!(probes, vec![keyboard_pairing()]);
        }
    }

    #[test]
    fn does_not_restore_old_features_for_a_replaced_pairing() {
        let endpoint = bolt_endpoint();
        let replacement = HidppDevice {
            name: "MX Keys Mini".to_string(),
            pairing_name: Some("MX Keys Mini".to_string()),
            battery_protocol: BatteryProtocol::Unknown,
            ..resolved_keyboard()
        };
        let current = vec![MonitoredEndpoint {
            endpoint: endpoint.clone(),
            devices: vec![resolved_keyboard()],
        }];
        let discovered = vec![MonitoredEndpoint {
            endpoint,
            devices: vec![replacement.clone()],
        }];

        let reconciled = reconcile_discovered_endpoints(current, discovered, |_| true);

        assert_eq!(reconciled[0].devices, vec![replacement]);
    }

    #[test]
    fn does_not_restore_devices_for_a_reused_hidraw_path() {
        let endpoint = bolt_endpoint();
        let current = vec![MonitoredEndpoint {
            endpoint: endpoint.clone(),
            devices: vec![resolved_keyboard()],
        }];
        let discovered = vec![MonitoredEndpoint {
            endpoint: HidrawEndpoint {
                product_id: 0xc52b,
                ..endpoint
            },
            devices: Vec::new(),
        }];

        let reconciled = reconcile_discovered_endpoints(current, discovered, |_| true);

        assert!(reconciled[0].devices.is_empty());
    }

    #[test]
    fn preserves_a_sleeping_receiver_device_during_rediscovery() {
        let endpoint = bolt_endpoint();
        let known_keyboard = resolved_keyboard();
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
        let warm_started = std::time::Instant::now();
        let warm_states = monitor.query();
        println!(
            "Warm Logitech query in {:?}: {warm_states:?}",
            warm_started.elapsed()
        );
        monitor.last_discovery = None;
        let rediscovery_started = std::time::Instant::now();
        let rediscovered_states = monitor.query();
        println!(
            "Logitech rediscovery in {:?}: {rediscovered_states:?}",
            rediscovery_started.elapsed()
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
